import { useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";
import { keySourceLabel } from "../lib/labels";
import type { Pong, UpdateStatus } from "../lib/types";

interface Props {
  pong: Pong | null;
}

/**
 * 检查更新的界面状态。
 *
 * 只到"告诉你有没有新版"为止，不下载也不安装 —— 更新包没有代码签名时，
 * 静默安装会被 SmartScreen 拦，那反而比手动下载更糟。
 */
type CheckState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "latest"; version: string }
  | { kind: "available"; status: UpdateStatus }
  | { kind: "error"; message: string };

/** 依赖清单。协议是核对过各自 Cargo.toml / package.json 的，不是凭印象写的。 */
const COMPONENTS = [
  { name: "Tauri 2", license: "Apache-2.0 OR MIT", usage: "桌面外壳与前后端 IPC" },
  { name: "russh", license: "Apache-2.0", usage: "SSH 协议（握手、认证、通道）" },
  { name: "russh-sftp", license: "Apache-2.0", usage: "SFTP 子系统" },
  { name: "xterm.js", license: "MIT", usage: "终端渲染" },
  { name: "React", license: "MIT", usage: "界面" },
  { name: "Zustand", license: "MIT", usage: "前端状态" },
  { name: "rusqlite", license: "MIT", usage: "本地 SQLite" },
  { name: "aes-gcm", license: "Apache-2.0 OR MIT", usage: "凭据字段加密" },
  { name: "Serde", license: "Apache-2.0 OR MIT", usage: "序列化" },
];

/** 把检查结果翻成一句话。状态机在外面，文案只在这里维护。 */
function describeCheck(state: CheckState): string {
  switch (state.kind) {
    case "idle":
      return "还没检查过。";
    case "checking":
      return "正在向 GitHub 查询…";
    case "latest":
      return `已是最新版本（v${state.version}）。`;
    case "available": {
      const { latest, publishedAt } = state.status;
      const when = formatDate(publishedAt);
      return `发现新版本 v${latest}${when ? `（发布于 ${when}）` : ""}。`;
    }
    case "error":
      return state.message;
  }
}

/** 2026-11-02T10:00:00Z → 2026-11-02（本地时区）。解析不出来就不显示。 */
function formatDate(raw: string | null): string | null {
  if (!raw) return null;
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) return null;
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

export default function AboutPage({ pong }: Props) {
  const [check, setCheck] = useState<CheckState>({ kind: "idle" });

  const runCheck = async () => {
    setCheck({ kind: "checking" });
    try {
      const status = await call<UpdateStatus>("update_check");
      if (status.hasUpdate && status.latest) {
        setCheck({ kind: "available", status });
      } else {
        // 远端没发过 release 时 latest 是 null，这时也算"已是最新"，
        // 不必让用户分清"没有新版"和"还没发布"
        setCheck({ kind: "latest", version: status.latest ?? status.current });
      }
    } catch (err) {
      setCheck({ kind: "error", message: fromUnknown(err).message });
    }
  };

  const openPage = () => {
    void call<void>("update_open_page").catch(() => undefined);
  };

  return (
    <div className="about-page">
      <header className="about__brand">
        <div className="about__mark">TH</div>
        <div>
          <h2 className="about__name">TermHarbor · 终端港</h2>
          <p className="about__tagline">
            跨平台 SSH 终端与文件管理客户端。连接、终端、传文件一条链路走完。
          </p>
        </div>
        <span className="about__version">v{pong?.version ?? "—"}</span>
      </header>

      <section className="about__section">
        <h3>运行信息</h3>
        <table className="about__table">
          <tbody>
            <tr>
              <th>版本</th>
              <td>{pong?.version ?? "读取中…"}</td>
            </tr>
            <tr>
              <th>配置库 schema</th>
              <td>
                {pong ? `v${pong.schemaVersion}` : "—"}
                {pong && pong.schemaVersion !== pong.latestSchemaVersion ? (
                  <span className="about__warn">
                    {pong.schemaVersion < pong.latestSchemaVersion
                      ? `（程序需要 v${pong.latestSchemaVersion}，重启后自动迁移）`
                      : `（比程序新，可能是装回了旧版本；程序只认 v${pong.latestSchemaVersion}）`}
                  </span>
                ) : null}
              </td>
            </tr>
            <tr>
              <th>密钥来源</th>
              <td title={pong?.keySource ?? undefined}>{keySourceLabel(pong?.keySource).text}</td>
            </tr>
            <tr>
              <th>数据目录</th>
              <td className="about__path">{pong?.dataDir ?? "—"}</td>
            </tr>
            <tr>
              <th>日志目录</th>
              <td className="about__path">{pong?.logDir ?? "—"}</td>
            </tr>
          </tbody>
        </table>
        <p className="about__hint">
          凭据以 AES-256-GCM 加密后存在数据目录的 SQLite 里，密钥在系统钥匙串；
          日志里的敏感字段在写入前已脱敏。
        </p>
      </section>

      <section className="about__section">
        <h3>更新</h3>
        <div className="about__update">
          <span className="about__update-text">{describeCheck(check)}</span>
          <button
            type="button"
            className="btn btn--sm"
            disabled={check.kind === "checking"}
            onClick={() => void runCheck()}
          >
            {check.kind === "checking" ? "检查中…" : "检查更新"}
          </button>
          {check.kind === "available" ? (
            <button type="button" className="btn btn--primary btn--sm" onClick={openPage}>
              打开下载页
            </button>
          ) : null}
        </div>
        <p className="about__hint">
          检查会请求 GitHub 的 releases 接口，只比对版本号，不上传任何信息。
          没有自动下载/静默安装 —— 安装包还没做代码签名，静默装会被 SmartScreen 拦。
          网络连不上 GitHub 时这里会直接报错，不影响其它功能。
        </p>
      </section>

      <section className="about__section">
        <h3>开源组件</h3>
        <table className="about__table about__table--grid">
          <thead>
            <tr>
              <th>组件</th>
              <th>协议</th>
              <th>用途</th>
            </tr>
          </thead>
          <tbody>
            {COMPONENTS.map((item) => (
              <tr key={item.name}>
                <td className="about__dep">{item.name}</td>
                <td className="about__license">{item.license}</td>
                <td>{item.usage}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="about__hint">
          完整依赖清单见仓库的 <code>src-tauri/Cargo.lock</code> 与 <code>pnpm-lock.yaml</code>。
          各组件版权归其作者所有。
        </p>
      </section>

      <section className="about__section">
        <h3>反馈</h3>
        <p className="about__hint">
          遇到问题时，请附上版本号与日志目录下的日志文件（默认保留 7 天）。
          日志已脱敏，但发送前仍建议自己扫一眼。
        </p>
      </section>
    </div>
  );
}
