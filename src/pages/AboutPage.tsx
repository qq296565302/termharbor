import { keySourceLabel } from "../lib/labels";
import type { Pong } from "../lib/types";

interface Props {
  pong: Pong | null;
}

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

export default function AboutPage({ pong }: Props) {
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
