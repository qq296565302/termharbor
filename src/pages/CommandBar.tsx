import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChangeEvent, KeyboardEvent, SyntheticEvent } from "react";

import {
  appendField,
  currentToken,
  mergeCandidates,
  replaceToken,
  resolvePathPrefix,
  splitFields,
} from "../lib/commandFields";
import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { HistoryEntry } from "../lib/types";

import "./command-bar.css";

export interface CommandBarProps {
  sessionId: string;
  connectionId: string;
  cwd: string;
  tabCount: number;
  onSend: (text: string, broadcast: boolean) => void;
}

/** 补全浮层里三种来源各自的徽标样式后缀。 */
const BADGE_SUFFIX: Record<string, string> = {
  "历史命令": "cmd",
  "访问过的路径": "path",
  "远端目录": "dir",
};

const LABEL_CMD = "历史命令";
const LABEL_PATH = "访问过的路径";
const LABEL_DIR = "远端目录";

interface Banner {
  text: string;
  tone: "danger" | "warn";
}

/** 把毫秒时间戳说成人话。历史不截断，条目可能很老，给到「年前」即可。 */
function relativeTime(tsMs: number): string {
  const sec = Math.max(0, Math.floor((Date.now() - tsMs) / 1000));
  if (sec < 60) return "刚刚";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day < 30) return `${day} 天前`;
  const month = Math.floor(day / 30);
  if (month < 12) return `${month} 个月前`;
  return `${Math.floor(day / 365)} 年前`;
}

/** path 历史本地按词头做子串匹配（大小写不敏感），与服务端模糊匹配保持一致。 */
function filterPaths(rows: HistoryEntry[], head: string): string[] {
  const key = head.toLowerCase();
  return rows.filter((row) => row.value.toLowerCase().includes(key)).map((row) => row.value);
}

export default function CommandBar(props: CommandBarProps): JSX.Element {
  const { sessionId, connectionId, cwd, tabCount, onSend } = props;

  const [value, setValue] = useState("");
  const [broadcast, setBroadcast] = useState(false);

  const [completions, setCompletions] = useState<{ label: string; value: string }[]>([]);
  const [completionOpen, setCompletionOpen] = useState(false);
  const [highlight, setHighlight] = useState(0);

  const [historyOpen, setHistoryOpen] = useState(false);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [historyQuery, setHistoryQuery] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [activeHistoryId, setActiveHistoryId] = useState<string | null>(null);
  const [fieldIndex, setFieldIndex] = useState(0);

  const [actionError, setActionError] = useState<Banner | null>(null);
  const [completionError, setCompletionError] = useState<string | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  /** 直接读最新输入与光标，避免防抖闭包拿到旧值。 */
  const valueRef = useRef(value);
  valueRef.current = value;
  const cursorRef = useRef(0);
  /** Esc / 发送 / 选中候选后置 false，防止 cwd 变化等重跑把浮层又弹出来。 */
  const completionArmedRef = useRef(false);
  /** 卸载标志：不用它，卸载后 setState 会报警。 */
  const aliveRef = useRef(true);
  /** 切标签时自增，让上一个会话的在途请求结果作废。 */
  const epochRef = useRef(0);
  /** 抽屉开着时不弹补全浮层，两块浮层都向上弹会互相遮挡。 */
  const historyOpenRef = useRef(false);
  historyOpenRef.current = historyOpen;

  useEffect(() => {
    aliveRef.current = true;
    return () => {
      aliveRef.current = false;
    };
  }, []);

  const reloadHistory = useCallback(async () => {
    const epoch = epochRef.current;
    try {
      const rows = await call<HistoryEntry[]>("history_list", { kind: "command" });
      if (!aliveRef.current || epoch !== epochRef.current) return;
      setHistory(rows);
      setActionError(null);
    } catch (err) {
      if (!aliveRef.current || epoch !== epochRef.current) return;
      setActionError({ text: `加载命令历史失败：${fromUnknown(err).message}`, tone: "danger" });
    }
  }, []);

  /**
   * sessionId / connectionId 变化 = 切了标签或连接：历史重新拉、补全浮层与
   * 字段选择全部复位。cwd 不放在这里 —— 它只影响补全（见下面的防抖依赖），
   * 每敲一次 cd 就把界面清空太打扰。
   */
  useEffect(() => {
    epochRef.current += 1;
    completionArmedRef.current = false;
    setCompletions([]);
    setCompletionOpen(false);
    setHighlight(0);
    setActiveHistoryId(null);
    setFieldIndex(0);
    setConfirmClear(false);
    setCompletionError(null);
    void reloadHistory();
  }, [sessionId, connectionId, reloadHistory]);

  /**
   * 拉候选。三组来源并行，单个失败不影响其余，最后合并去重。
   * 依赖里带 sessionId / cwd：换标签、cd 之后都会用新值重新拉（补全基于新 cwd）。
   */
  const runComplete = useCallback(async () => {
    if (!completionArmedRef.current || !aliveRef.current || historyOpenRef.current) return;
    const epoch = epochRef.current;
    const line = valueRef.current;
    if (!line.trim()) {
      setCompletions([]);
      setCompletionOpen(false);
      setCompletionError(null);
      return;
    }
    const token = currentToken(line, Math.min(cursorRef.current, line.length));
    const needDir = token.text.includes("/") || token.text.includes(".");
    const prefix = needDir ? resolvePathPrefix(cwd, token.text) : "";

    const settled = await Promise.allSettled([
      call<string[]>("history_complete", { kind: "command", query: token.text, limit: 30 }),
      call<HistoryEntry[]>("history_list", { kind: "path" }),
      needDir
        ? call<string[]>("path_complete", { sessionId, prefix, limit: 30 })
        : Promise.resolve<string[]>([]),
    ] as const);

    if (!aliveRef.current || epoch !== epochRef.current) return;

    const slots: { label: string; values: string[] }[] = [
      { label: LABEL_CMD, values: [] },
      { label: LABEL_PATH, values: [] },
      { label: LABEL_DIR, values: [] },
    ];
    const failures: string[] = [];
    if (settled[0].status === "fulfilled") slots[0].values = settled[0].value;
    else failures.push(fromUnknown(settled[0].reason).message);
    if (settled[1].status === "fulfilled") {
      slots[1].values = filterPaths(settled[1].value, token.text);
    } else {
      failures.push(fromUnknown(settled[1].reason).message);
    }
    if (needDir) {
      if (settled[2].status === "fulfilled") slots[2].values = settled[2].value;
      else failures.push(fromUnknown(settled[2].reason).message);
    }

    const merged = mergeCandidates(slots, 30);
    setCompletions(merged);
    setHighlight(0);
    setCompletionOpen(merged.length > 0);
    setCompletionError(failures.length > 0 ? `补全部分不可用：${failures[0]}` : null);
  }, [sessionId, cwd]);

  /** 输入变化防抖 180ms；cleanup 清 timer，避免卸载/快速输入后旧结果回写。 */
  useEffect(() => {
    const timer = window.setTimeout(() => {
      void runComplete();
    }, 180);
    return () => window.clearTimeout(timer);
  }, [value, runComplete]);

  const activeEntry = useMemo(
    () => history.find((row) => row.id === activeHistoryId) ?? null,
    [history, activeHistoryId],
  );
  const activeFields = useMemo(
    () => (activeEntry ? splitFields(activeEntry.value) : []),
    [activeEntry],
  );
  const filteredHistory = useMemo(() => {
    const key = historyQuery.trim().toLowerCase();
    if (!key) return history;
    return history.filter((row) => row.value.toLowerCase().includes(key));
  }, [history, historyQuery]);

  const banner: Banner | null =
    actionError ?? (completionError ? { text: completionError, tone: "warn" } : null);

  const closeCompletion = () => {
    completionArmedRef.current = false;
    setCompletionOpen(false);
  };

  const applyCompletion = (candidate: { label: string; value: string }) => {
    const line = valueRef.current;
    const token = currentToken(line, Math.min(cursorRef.current, line.length));
    // 远端目录返回的是完整路径，替换整个 token；其余候选同理按词头整段替换
    const { text, cursor } = replaceToken(line, token.start, token.end, candidate.value);
    setValue(text);
    valueRef.current = text;
    cursorRef.current = cursor;
    closeCompletion();
    setCompletions([]);
    setHighlight(0);
    // 受控 input 要先渲染新值，下一帧再把光标放到替换文本之后
    window.requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(cursor, cursor);
    });
  };

  /** 把一个字段追加到输入框末尾（空格分隔），并把焦点交还输入框。 */
  const insertField = (entry: HistoryEntry, field: string) => {
    const next = appendField(valueRef.current, field);
    setValue(next);
    valueRef.current = next;
    cursorRef.current = next.length;
    setActiveHistoryId(entry.id);
    closeCompletion();
    window.requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(next.length, next.length);
    });
  };

  const send = () => {
    const text = value.trim();
    if (!text) return;
    // 命令先发出去：history_push 失败只提示，绝不因为记历史没成功就阻断发送
    onSend(text, broadcast);
    setValue("");
    valueRef.current = "";
    cursorRef.current = 0;
    closeCompletion();
    setCompletions([]);
    setHighlight(0);
    setActionError(null);

    const epoch = epochRef.current;
    void (async () => {
      try {
        await call<void>("history_push", { kind: "command", value: text, connectionId });
        if (!aliveRef.current || epoch !== epochRef.current) return;
        await reloadHistory();
      } catch (err) {
        if (!aliveRef.current || epoch !== epochRef.current) return;
        setActionError({
          text: `命令已发送，但记录历史失败：${fromUnknown(err).message}`,
          tone: "warn",
        });
      }
    })();

    inputRef.current?.focus();
  };

  const onInputKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    // 字段选择快捷键：Alt+左右在浏览器里是前进/后退，必须 preventDefault，
    // 否则会触发导航。只在历史里选中了一条命令时才真正移动/插入字段。
    if (
      event.altKey &&
      (event.key === "ArrowLeft" || event.key === "ArrowRight" || event.key === "Enter")
    ) {
      event.preventDefault();
      if (!activeEntry || activeFields.length === 0) return;
      if (event.key === "Enter") {
        const field = activeFields[fieldIndex] ?? activeFields[0];
        if (field) insertField(activeEntry, field.text);
        return;
      }
      const step = event.key === "ArrowRight" ? 1 : -1;
      setFieldIndex((prev) => (prev + step + activeFields.length) % activeFields.length);
      return;
    }

    // 浮层打开时才接管方向键 / Tab / Enter。
    // 组件挂在 xterm 外面，绝不在 document 上挂监听：浮层关着时 ↑↓ 要留给
    // 用户在终端里翻历史、移动光标，Tab 也要能正常把焦点移走。
    if (completionOpen && completions.length > 0) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setHighlight((h) => (h + 1) % completions.length);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        setHighlight((h) => (h - 1 + completions.length) % completions.length);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        closeCompletion();
        return;
      }
      if (event.key === "Tab" || (event.key === "Enter" && !event.shiftKey)) {
        event.preventDefault();
        applyCompletion(completions[highlight]);
        return;
      }
    } else if (event.key === "Escape" && completionOpen) {
      closeCompletion();
      return;
    }

    if (event.key === "Enter") {
      if (event.shiftKey) return; // Shift+Enter 不发送
      event.preventDefault();
      send();
    }
  };

  const fillFromHistory = (entry: HistoryEntry) => {
    // 点击整条 = 填入并选中（选中后才能用 Alt 快捷键在该条字段间挑），随后聚焦输入框
    setValue(entry.value);
    valueRef.current = entry.value;
    cursorRef.current = entry.value.length;
    setActiveHistoryId(entry.id);
    setFieldIndex(0);
    closeCompletion();
    window.requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(entry.value.length, entry.value.length);
    });
  };

  const removeHistory = async (id: string) => {
    const epoch = epochRef.current;
    try {
      await call<void>("history_remove", { kind: "command", id });
      if (!aliveRef.current || epoch !== epochRef.current) return;
      if (activeHistoryId === id) {
        setActiveHistoryId(null);
        setFieldIndex(0);
      }
      await reloadHistory();
    } catch (err) {
      if (!aliveRef.current || epoch !== epochRef.current) return;
      setActionError({ text: `删除历史失败：${fromUnknown(err).message}`, tone: "danger" });
    }
  };

  const clearHistory = async () => {
    const epoch = epochRef.current;
    try {
      await call<void>("history_clear", { kind: "command" });
      if (!aliveRef.current || epoch !== epochRef.current) return;
      setConfirmClear(false);
      setActiveHistoryId(null);
      setFieldIndex(0);
      await reloadHistory();
    } catch (err) {
      if (!aliveRef.current || epoch !== epochRef.current) return;
      setActionError({ text: `清空历史失败：${fromUnknown(err).message}`, tone: "danger" });
    }
  };

  const onChange = (event: ChangeEvent<HTMLInputElement>) => {
    const next = event.target.value;
    setValue(next);
    valueRef.current = next;
    cursorRef.current = event.target.selectionStart ?? next.length;
    completionArmedRef.current = true;
  };

  const syncCursor = (event: SyntheticEvent<HTMLInputElement>) => {
    cursorRef.current = event.currentTarget.selectionStart ?? 0;
  };

  return (
    <div className="cb">
      {banner ? (
        <div className={`cb__banner cb__banner--${banner.tone}`} role="alert">
          <span className="cb__banner-text">{banner.text}</span>
          <button
            type="button"
            className="cb__banner-close"
            aria-label="关闭提示"
            onClick={() => {
              setActionError(null);
              setCompletionError(null);
            }}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M4.2 4.2l7.6 7.6M11.8 4.2l-7.6 7.6" />
            </svg>
          </button>
        </div>
      ) : null}

      {/* 补全浮层：向上弹，避免盖住终端区域 */}
      {completionOpen && completions.length > 0 ? (
        <ul className="cb__suggest" role="listbox" aria-label="命令与路径候选">
          {completions.map((cand, index) => (
            <li key={cand.value} role="presentation">
              <button
                type="button"
                className={`cb__suggest-item${index === highlight ? " is-active" : ""}`}
                role="option"
                aria-selected={index === highlight}
                onMouseDown={(e) => e.preventDefault()}
                onMouseEnter={() => setHighlight(index)}
                onClick={() => applyCompletion(cand)}
              >
                <span className={`cb__badge cb__badge--${BADGE_SUFFIX[cand.label] ?? "cmd"}`}>
                  {cand.label}
                </span>
                <code className="cb__suggest-value">{cand.value}</code>
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      {/* 历史抽屉：数据不截断，靠 max-height + overflow:auto 承载全部记录 */}
      {historyOpen ? (
        <section className="cb__drawer" aria-label="命令历史">
          <header className="cb__drawer-head">
            <span className="cb__drawer-title">命令历史</span>
            <span className="cb__drawer-count">{history.length} 条</span>
            <span className="cb__spacer" />
            {confirmClear ? (
              <>
                <button
                  type="button"
                  className="btn btn--small btn--danger"
                  onClick={() => void clearHistory()}
                >
                  确认清空
                </button>
                <button
                  type="button"
                  className="btn btn--small btn--ghost"
                  onClick={() => setConfirmClear(false)}
                >
                  取消
                </button>
              </>
            ) : (
              <button
                type="button"
                className="btn btn--small btn--ghost"
                onClick={() => setConfirmClear(true)}
              >
                清空全部
              </button>
            )}
            <button
              type="button"
              className="btn btn--small btn--ghost btn--icon"
              aria-label="关闭历史"
              onClick={() => {
                setHistoryOpen(false);
                setConfirmClear(false);
              }}
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M4.2 4.2l7.6 7.6M11.8 4.2l-7.6 7.6" />
              </svg>
            </button>
          </header>

          <div className="cb__drawer-searchbox">
            <svg className="cb__searchicon" viewBox="0 0 16 16" aria-hidden="true">
              <circle cx="7.1" cy="7.1" r="4.1" />
              <path d="m10.2 10.2 3 3" />
            </svg>
            <input
              className="input input--search cb__drawer-search"
              placeholder="搜索历史命令"
              value={historyQuery}
              onChange={(e) => setHistoryQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setHistoryOpen(false);
                  setConfirmClear(false);
                }
              }}
            />
          </div>

          <p className="cb__hint">
            点击命令填入输入框并选中；点击字段 chip 只追加该字段。选中命令后按 Alt + ← / Alt + →
            在字段间移动高亮，Alt + Enter 插入当前字段。
          </p>

          <ul className="cb__hist-list">
            {filteredHistory.length === 0 ? (
              <li className="cb__empty">没有匹配的历史命令</li>
            ) : null}
            {filteredHistory.map((entry) => {
              const fields = splitFields(entry.value);
              const isActive = entry.id === activeHistoryId;
              return (
                <li key={entry.id} className={`cb__hist-item${isActive ? " is-active" : ""}`}>
                  <div className="cb__hist-main">
                    <button
                      type="button"
                      className="cb__hist-value"
                      title="填入输入框并选中"
                      onClick={() => fillFromHistory(entry)}
                    >
                      <code>{entry.value}</code>
                    </button>
                    <span className="cb__hist-meta">
                      <span className="cb__hist-time">{relativeTime(entry.tsMs)}</span>
                      <span className="cb__hist-count">×{entry.useCount}</span>
                      <button
                        type="button"
                        className="cb__hist-del"
                        title="删除这条"
                        aria-label="删除这条"
                        onClick={() => void removeHistory(entry.id)}
                      >
                        <svg viewBox="0 0 16 16" aria-hidden="true">
                          <path d="M3 4.6h10" />
                          <path d="M6.4 4.6V3.2h3.2v1.4" />
                          <path d="M4.4 4.6 5 12.3a1 1 0 0 0 1 .9h4a1 1 0 0 0 1-.9l.6-7.7" />
                        </svg>
                      </button>
                    </span>
                  </div>
                  <div className="cb__chips">
                    {fields.map((field, index) => (
                      <button
                        type="button"
                        key={`${field.start}-${field.text}`}
                        className={`cb__chip${isActive && index === fieldIndex ? " is-active" : ""}`}
                        title="只追加这个字段到输入框"
                        onClick={() => {
                          setFieldIndex(index);
                          insertField(entry, field.text);
                        }}
                      >
                        {field.text}
                      </button>
                    ))}
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}

      <div className="cb__row">
        <div className="cb__input-wrap">
          <input
            ref={inputRef}
            className="input cb__input"
            placeholder="输入命令，Enter 发送，Shift+Enter 不发送"
            value={value}
            onChange={onChange}
            onKeyDown={onInputKeyDown}
            onSelect={syncCursor}
            onClick={syncCursor}
            onKeyUp={syncCursor}
            onBlur={closeCompletion}
            autoComplete="off"
            spellCheck={false}
            aria-label="命令输入"
          />
        </div>
        <button
          type="button"
          className="btn btn--primary cb__send"
          disabled={!value.trim()}
          onClick={send}
          title="发送（Enter）"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M13.5 2.5 2.5 7.2l4.3 1.6 1.6 4.3z" />
            <path d="m6.8 8.8 6.7-6.3" />
          </svg>
          发送
        </button>
        <button
          type="button"
          className={`btn btn--ghost cb__history-btn${historyOpen ? " is-on" : ""}`}
          onClick={() => {
            setHistoryOpen((open) => !open);
            setConfirmClear(false);
            closeCompletion();
            if (!historyOpen) void reloadHistory();
          }}
          title="打开命令历史"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="8" cy="8" r="5.6" />
            <path d="M8 4.7V8l2.3 1.5" />
          </svg>
          历史
        </button>
      </div>

      <label className="cb__broadcast">
        <input
          type="checkbox"
          checked={broadcast}
          onChange={(event) => setBroadcast(event.target.checked)}
        />
        <span className="cb__broadcast-main">同步到全部标签（{tabCount}）</span>
        <span className="cb__broadcast-sub">命令同步不限制标签数量，一键下发到所有会话</span>
      </label>
    </div>
  );
}
