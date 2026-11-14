/**
 * 「待确认操作」卡片：把被高危写入闸门拦下的调用摆到前台，一键放行。
 *
 * 为什么不做一个独立的 pending 队列：MCP 是 stdio 串行服务，被拒的那次调用
 * 已经结束了，没有「挂起等回调」的句柄。审计日志里那条 `system_path_write`
 * 的拒绝记录就是天然的待办项 —— 复用它，GUI 和 MCP 之间不用再维护第二份状态。
 *
 * 放行写进 mcp.json 的 approvals，MCP 侧按 mtime 热加载读到，模型重试时自然通过。
 * 用户看到的是「点一次就好」，不是又一张要逐项勾选的白名单。
 */

import { useMemo, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { McpApproval, McpAuditEntry } from "../lib/types";
import "./mcp-approval.css";

const SCOPE = "system_write";

interface Props {
  entries: McpAuditEntry[];
  approvals: McpApproval[];
  /** 放行 / 撤销后让父组件重新拉配置与审计 */
  onChanged: () => void;
}

function timeText(ms: number): string {
  return new Date(ms).toLocaleTimeString("zh-CN", { hour12: false });
}

export default function McpApprovalCard({ entries, approvals, onChanged }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** 每台机器只留最近一次被拦的调用；同一台连着试十次不该刷出十行。 */
  const pending = useMemo(() => {
    const seen = new Set<string>();
    const out: McpAuditEntry[] = [];
    for (const entry of [...entries].sort((a, b) => b.tsMs - a.tsMs)) {
      if (entry.reason !== SCOPE || entry.decision !== "denied") continue;
      if (seen.has(entry.connection)) continue;
      seen.add(entry.connection);
      out.push(entry);
      if (out.length >= 8) break;
    }
    return out;
  }, [entries]);

  const active = useMemo(
    () => approvals.filter((item) => item.scope === SCOPE && item.untilMs > Date.now()),
    [approvals],
  );

  const grant = async (connection: string) => {
    setBusy(true);
    setError(null);
    try {
      await call("mcp_approval_grant", { connection, scope: SCOPE });
      onChanged();
    } catch (err) {
      setError(fromUnknown(err).message);
    } finally {
      setBusy(false);
    }
  };

  const revoke = async () => {
    setBusy(true);
    setError(null);
    try {
      await call("mcp_approval_revoke");
      onChanged();
    } catch (err) {
      setError(fromUnknown(err).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="approval">
      <div className="approval__head">
        <span className="approval__title">待确认操作</span>
        {active.length > 0 ? (
          <button
            type="button"
            className="btn btn--ghost btn--small"
            disabled={busy}
            onClick={() => void revoke()}
          >
            撤销全部放行
          </button>
        ) : null}
      </div>
      <p className="approval__hint">
        写 systemd 单元、定时任务、提权或登录配置这类「系统会自动执行」的位置会先拦一下。
        读文件、写业务目录都不拦；点一次放行管到本次客户端会话结束。
      </p>

      {active.length > 0 ? (
        <p className="approval__active">
          当前有 {active.length} 台机器已放行，到 {timeText(Math.max(...active.map((a) => a.untilMs)))} 失效。
        </p>
      ) : null}

      {pending.length === 0 ? (
        <p className="approval__empty">暂时没有需要你确认的写入。</p>
      ) : (
        <ul className="approval__list">
          {pending.map((entry) => (
            <li className="approval__row" key={`${entry.connection}-${entry.tsMs}`}>
              <span className="approval__time">{timeText(entry.tsMs)}</span>
              <span className="approval__conn" title={entry.connection}>
                {entry.connection}
              </span>
              <span className="approval__args" title={entry.args}>
                {entry.args}
              </span>
              <button
                type="button"
                className="btn btn--small btn--primary"
                disabled={busy}
                onClick={() => void grant(entry.connection)}
              >
                允许这一次
              </button>
            </li>
          ))}
        </ul>
      )}

      {error ? <p className="approval__error">{error}</p> : null}
    </section>
  );
}
