import { useCallback, useEffect, useRef, useState } from "react";

import { AppError, fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { DbAccountDto, DbQueryResult, SchemaSnapshot, TableSchema } from "../lib/types";

/** 表格一次最多渲染这么多行。数据本身有 1MB 上限，但 2 万行 DOM 会明显卡。 */
const MAX_ROWS = 500;

interface Props {
  connectionId: string | null;
  onClose: () => void;
}

/**
 * 数据库面板：偶尔查一条数据用的轻量客户端。
 *
 * 不是 Navicat 的替代品 —— 只有「写 SQL、看结果」，没有表结构树、没有行内编辑。
 * 后端借 SSH 通道在服务器上跑 mysql 客户端，所以这里不需要选驱动、也不需要
 * 先配 SSH 隧道。
 */
export default function DbPanel({ connectionId, onClose }: Props) {
  const [account, setAccount] = useState<DbAccountDto | null>(null);
  const [editing, setEditing] = useState(false);
  const [form, setForm] = useState({ username: "", password: "", port: "3306", database: "" });
  const [sql, setSql] = useState("select 1");
  const [result, setResult] = useState<DbQueryResult | null>(null);
  const [schema, setSchema] = useState<SchemaSnapshot | null>(null);
  const [schemaBusy, setSchemaBusy] = useState(false);
  const [showTables, setShowTables] = useState(true);
  const [databases, setDatabases] = useState<string[]>([]);
  /** 面板当前看着哪个库。只影响这个面板，不写回保存的默认库。 */
  const [activeDatabase, setActiveDatabase] = useState("");
  /**
   * 拉库列表 / 表结构失败的原因。
   *
   * 之前这里是静默 catch —— 下拉空着、表也列不出来，但用户看不到任何原因，
   * 只能猜是软件坏了。凡是"该有东西却没有"的地方，都得把原因说出来。
   */
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** 回车连按时挡住第二次：setBusy 是异步的，光看 state 会漏 */
  const inFlight = useRef(false);

  const load = useCallback(async (id: string) => {
    try {
      const dto = await call<DbAccountDto>("db_account_get", { connectionId: id });
      setAccount(dto);
      setForm({
        username: dto.username,
        // 后端只回 configured，不回密码 —— 改配置时要重新输一次
        password: "",
        port: String(dto.port),
        database: dto.database,
      });
      setEditing(!dto.configured);
    } catch (err) {
      setError(textOf(err));
    }
  }, []);

  useEffect(() => {
    setResult(null);
    setError(null);
    if (!connectionId) {
      setAccount(null);
      return;
    }
    void load(connectionId);
  }, [connectionId, load]);

  const loadSchema = useCallback(
    async (id: string, database: string, refresh = false) => {
      if (!database) {
        setSchema(null);
        return;
      }
      setSchemaBusy(true);
      try {
        const snapshot = await call<SchemaSnapshot>("db_schema", {
          connectionId: id,
          database,
          refresh,
        });
        setSchema(snapshot);
        setLoadError(null);
      } catch (err) {
        // 拿不到表结构不挡住查询本身，但要把原因说出来
        setSchema(null);
        setLoadError(textOf(err));
      } finally {
        setSchemaBusy(false);
      }
    },
    [],
  );

  const loadDatabases = useCallback(async (id: string, fallback: string) => {
    try {
      const list = await call<string[]>("db_databases", { connectionId: id });
      setDatabases(list);
      setLoadError(null);
      // 没配默认库、或默认库已经不在列表里，就落到第一个 —— 省得用户
      // 先手动选一次才能看到表。
      setActiveDatabase((prev) => {
        if (prev && list.includes(prev)) return prev;
        if (fallback && list.includes(fallback)) return fallback;
        return list[0] ?? "";
      });
    } catch (err) {
      setDatabases([]);
      setLoadError(textOf(err));
    }
  }, []);

  useEffect(() => {
    if (!connectionId || !account?.configured) {
      setDatabases([]);
      setActiveDatabase("");
      return;
    }
    void loadDatabases(connectionId, account.database);
  }, [connectionId, account?.configured, account?.database, loadDatabases]);

  useEffect(() => {
    if (!connectionId || !activeDatabase) {
      setSchema(null);
      return;
    }
    void loadSchema(connectionId, activeDatabase);
  }, [connectionId, activeDatabase, loadSchema]);

  /** 点表名填一条查询。
   *
   * 只在面板还是空的（或还是默认的 `select 1`）时替换，否则追加 ——
   * 用户正在写一半的 SQL 不能被一次误点冲掉。
   */
  const insertTable = (table: TableSchema) => {
    const qualified = activeDatabase ? `${activeDatabase}.${table.name}` : table.name;
    const statement = `select * from ${qualified} limit 20`;
    setSql((prev) => {
      const trimmed = prev.trim();
      if (!trimmed || trimmed === "select 1") return statement;
      return `${prev.trimEnd()}\n${statement}`;
    });
  };

  const save = async () => {
    if (!connectionId || busy) return;
    setBusy(true);
    setError(null);
    try {
      const dto = await call<DbAccountDto>("db_account_save", {
        connectionId,
        input: {
          username: form.username,
          password: form.password,
          port: Number(form.port) || 3306,
          database: form.database,
        },
      });
      setAccount(dto);
      setEditing(false);
    } catch (err) {
      setError(textOf(err));
    } finally {
      setBusy(false);
    }
  };

  const run = async () => {
    if (!connectionId || inFlight.current || !sql.trim()) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const res = await call<DbQueryResult>("db_query", {
        connectionId,
        sql,
        database: activeDatabase || undefined,
      });
      setResult(res);
    } catch (err) {
      setResult(null);
      setError(textOf(err));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  return (
    <aside className="term-db">
      <div className="term-db__head">
        <span className="term-db__title">数据库</span>
        {account?.configured ? (
          <select
            className="term-db__db"
            value={activeDatabase}
            disabled={databases.length === 0}
            onChange={(e) => {
              setActiveDatabase(e.target.value);
              setResult(null);
              setError(null);
            }}
            title="切换要看的库（只影响这个面板，不改保存的默认库）"
          >
            {databases.length === 0 ? <option value="">—</option> : null}
            {databases.map((db) => (
              <option key={db} value={db}>
                {db}
              </option>
            ))}
          </select>
        ) : null}
        <span className="term-db__meta" title={metaTitle(account)}>
          {metaText(account)}
        </span>
        {account?.configured ? (
          <button
            type="button"
            className="term-db__icon"
            title={editing ? "取消修改" : "改数据库账号"}
            aria-label={editing ? "取消修改" : "改数据库账号"}
            onClick={() => {
              setEditing((prev) => !prev);
              setError(null);
            }}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <circle cx="8" cy="8" r="2.2" />
              <path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1" />
            </svg>
          </button>
        ) : null}
        <button
          type="button"
          className="term-db__icon"
          title="关闭"
          aria-label="关闭数据库面板"
          onClick={onClose}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4 4l8 8M12 4l-8 8" />
          </svg>
        </button>
      </div>

      {!connectionId ? (
        <p className="db-empty">先连上一台服务器，数据库面板跟着当前标签走。</p>
      ) : account === null ? (
        <p className="db-empty">读取账号配置…</p>
      ) : editing || !account.configured ? (
        <>
          <p className="db-tip">
            填服务器上 MySQL 的账号。密码存在本机加密存储里，和 SSH 凭据同一套；
            查询走 SSH 通道，不经过公网。密码留空则用服务器的 socket 认证 ——
            root 配成 auth_socket 时就该留空。
          </p>
          <div className="db-setup">
            <label className="db-field">
              <span>用户名</span>
              <input
                className="input"
                value={form.username}
                placeholder="root"
                onChange={(e) => setForm({ ...form, username: e.target.value })}
              />
            </label>
            <label className="db-field">
              <span>密码</span>
              <input
                className="input"
                type="password"
                value={form.password}
                placeholder={account.configured ? "重新输入（不回显）" : "留空 = socket 认证"}
                onChange={(e) => setForm({ ...form, password: e.target.value })}
              />
            </label>
            <label className="db-field">
              <span>端口</span>
              <input
                className="input"
                inputMode="numeric"
                value={form.port}
                onChange={(e) => setForm({ ...form, port: e.target.value.replace(/\D/g, "") })}
              />
            </label>
            <label className="db-field">
              <span>默认库（可空）</span>
              <input
                className="input"
                value={form.database}
                placeholder="sgcc_ops"
                onChange={(e) => setForm({ ...form, database: e.target.value })}
              />
            </label>
          </div>
          <div className="db-setup__actions">
            {account.configured ? (
              <button
                type="button"
                className="btn btn--ghost btn--small"
                disabled={busy}
                onClick={() => {
                  setEditing(false);
                  setError(null);
                }}
              >
                取消
              </button>
            ) : null}
            <button
              type="button"
              className="btn btn--primary btn--small"
              disabled={busy}
              onClick={() => void save()}
            >
              保存
            </button>
          </div>
          {error ? <p className="db-error">{error}</p> : null}
        </>
      ) : (
        <>
          <div className="db-tables">
            <div className="db-tables__head">
              <span className="db-tables__title">
                {schema ? `表 ${schema.tables.length}` : "表"}
                {schema?.truncated ? "（已截断）" : ""}
              </span>
              <button
                type="button"
                className="db-tables__action"
                disabled={schemaBusy}
                onClick={() => void loadSchema(connectionId, activeDatabase, true)}
              >
                {schemaBusy ? "读取中…" : "刷新"}
              </button>
              <button
                type="button"
                className="db-tables__action"
                onClick={() => setShowTables((prev) => !prev)}
              >
                {showTables ? "收起" : "展开"}
              </button>
            </div>
            {showTables ? (
              schema && schema.tables.length > 0 ? (
                <div className="db-tables__list">
                  {schema.tables.map((table) => (
                    <button
                      key={table.name}
                      type="button"
                      className="db-tables__item"
                      title={`${table.name}（${table.columns.length} 列）`}
                      onClick={() => insertTable(table)}
                    >
                      {table.name}
                    </button>
                  ))}
                </div>
              ) : (
                <p
                  className={
                    loadError ? "db-tables__hint db-tables__hint--error" : "db-tables__hint"
                  }
                >
                  {loadError
                    ? `读不到库列表：${loadError}`
                    : schemaBusy
                      ? "读取表结构…"
                      : activeDatabase
                        ? "这个库里没有表，或者当前账号读不到 information_schema"
                        : "没能确定要看的库"}
                </p>
              )
            ) : null}
          </div>

          <div className="db-editor">
            <textarea
              className="db-editor__input"
              spellCheck={false}
              value={sql}
              onChange={(e) => setSql(e.target.value)}
              onKeyDown={(e) => {
                if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
                  e.preventDefault();
                  void run();
                }
              }}
              placeholder="select * from t limit 20"
            />
            <div className="db-editor__bar">
              <span className="db-editor__hint">Ctrl+Enter 执行</span>
              <button
                type="button"
                className="btn btn--primary btn--small"
                disabled={busy || !sql.trim()}
                onClick={() => void run()}
              >
                {busy ? "执行中…" : "执行"}
              </button>
            </div>
          </div>

          {error ? <p className="db-error">{error}</p> : null}

          <div className="db-result">
            {result ? <ResultTable result={result} /> : null}
          </div>

          <div className="db-status">
            {result
              ? `${result.rows.length} 行 · ${result.elapsedMs} ms${
                  result.truncated ? " · 输出过大已截断" : ""
                }`
              : "尚未执行"}
          </div>
        </>
      )}
    </aside>
  );
}

function ResultTable({ result }: { result: DbQueryResult }) {
  if (result.columns.length === 0) {
    return <p className="db-empty">执行成功，这条语句没有结果集。</p>;
  }
  if (result.rows.length === 0) {
    return <p className="db-empty">查到了 0 行。</p>;
  }

  const rows = result.rows.slice(0, MAX_ROWS);

  return (
    <div className="db-table-wrap">
      <table className="db-table">
        <thead>
          <tr>
            {result.columns.map((column, index) => (
              <th key={`${column}-${index}`} title={column}>
                {column}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, rowIndex) => (
            <tr key={rowIndex}>
              {row.map((cell, cellIndex) => (
                <td
                  key={cellIndex}
                  className={cell === "NULL" ? "is-null" : undefined}
                  title={cell.length > 60 ? cell : undefined}
                >
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {result.rows.length > MAX_ROWS ? (
        <p className="db-more">
          共 {result.rows.length} 行，表格只显示前 {MAX_ROWS} 行 —— 加个 LIMIT 会更快。
        </p>
      ) : null}
    </div>
  );
}

function textOf(err: unknown): string {
  const base = fromUnknown(err).message;
  return err instanceof AppError && err.detail ? err.detail : base;
}

function metaText(account: DbAccountDto | null): string {
  if (!account) return "";
  if (!account.configured) return "未配置";
  // 库名已经在上面的下拉里了，这里只留账号和端口
  return `${account.username}:${account.port}`;
}

function metaTitle(account: DbAccountDto | null): string {
  if (!account?.configured) return "";
  return `账号 ${account.username}，端口 ${account.port}`;
}
