import { Channel } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";

import type { PingResult } from "../hooks/usePing";
import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import type { Heartbeat } from "../lib/types";

const MILESTONE_ITEMS = [
  "工程骨架与需求 4.4 的模块划分",
  "统一错误码 AppError + 结构化序列化",
  "日志落盘与凭据脱敏",
  "AES-256-GCM 字段加密 + 密钥来源抽象",
  "SQLite schema 迁移框架（user_version）",
  "Command 请求响应 + Channel 高频推送两条链路",
  "三平台 CI 矩阵",
];

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="kv__row">
      <dt>{label}</dt>
      <dd className={mono ? "mono" : undefined}>{value}</dd>
    </div>
  );
}

/**
 * M0 自检页。
 *
 * 这个页面就是 M0 的验收界面：能读到版本与 schema、能拿到密钥来源、
 * 并且 Channel 能持续推心跳 —— 说明前后端两条通信链路都活着。
 * M2 起它会被真正的连接与终端页取代。
 */
export default function SelfCheckPage({ pong, error, loading, refresh }: PingResult) {
  const [ticks, setTicks] = useState<number[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [channelError, setChannelError] = useState<string | null>(null);

  const startHeartbeat = useCallback(async () => {
    try {
      const channel = new Channel<Heartbeat>();
      channel.onmessage = (hb) => {
        setTicks((prev) => [hb.tick, ...prev].slice(0, 8));
      };
      await call<void>("start_heartbeat", { channel });
      setStreaming(true);
      setChannelError(null);
    } catch (err) {
      setStreaming(false);
      setChannelError(fromUnknown(err).message);
    }
  }, []);

  const stopHeartbeat = useCallback(async () => {
    try {
      await call<void>("stop_heartbeat");
    } catch {
      // 停止失败不影响 UI，后端任务在窗口关闭时会自行退出
    }
    setStreaming(false);
  }, []);

  useEffect(() => {
    return () => {
      void call<void>("stop_heartbeat").catch(() => undefined);
    };
  }, []);

  return (
    <div className="page">
      <section className="card">
        <header className="card__head">
          <h2>后端连通性</h2>
          <button type="button" className="btn" onClick={refresh} disabled={loading}>
            {loading ? "检测中…" : "重新检测"}
          </button>
        </header>

        {error ? <div className="alert alert--danger">{error.message}</div> : null}

        {pong ? (
          <dl className="kv">
            <Row label="产品" value={`${pong.product} v${pong.version}`} />
            <Row
              label="数据库 schema"
              value={`v${pong.schemaVersion} / 最新 v${pong.latestSchemaVersion}`}
              mono
            />
            <Row label="密钥来源" value={pong.keySource} mono />
            <Row
              label="加密状态"
              value={pong.locked ? "未解锁（密钥不可用）" : "已解锁"}
            />
            <Row label="数据目录" value={pong.dataDir} mono />
            <Row label="日志目录" value={pong.logDir} mono />
          </dl>
        ) : loading ? (
          <div className="skeleton">正在与 Rust 侧握手…</div>
        ) : null}
      </section>

      <section className="card">
        <header className="card__head">
          <h2>Channel 通道</h2>
          <button
            type="button"
            className={`btn ${streaming ? "btn--danger" : "btn--primary"}`}
            onClick={streaming ? stopHeartbeat : startHeartbeat}
          >
            {streaming ? "停止推送" : "开始推送"}
          </button>
        </header>
        <p className="card__hint">
          每秒从 Rust 推一条心跳到渲染层。终端输出流（M2）走的是同一个机制，只是载荷换成
          <code>String</code> 并附带背压。
        </p>

        {channelError ? <div className="alert alert--danger">{channelError}</div> : null}

        <div className="ticks">
          <span className="ticks__label">最近心跳</span>
          <div className="ticks__list">
            {ticks.length === 0 ? (
              <span className="ticks__empty">尚未收到数据</span>
            ) : (
              ticks.map((tick) => (
                <span key={tick} className="tick">
                  #{tick}
                </span>
              ))
            )}
          </div>
        </div>
      </section>

      <section className="card">
        <header className="card__head">
          <h2>M0 已落地</h2>
        </header>
        <ul className="checklist">
          {MILESTONE_ITEMS.map((item) => (
            <li key={item}>
              <span className="checklist__mark" aria-hidden="true" />
              {item}
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
