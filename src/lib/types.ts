/** 与 Rust 侧 DTO 一一对应的类型。统一 camelCase（Rust 侧用 `serde(rename_all = "camelCase")` 转换）。 */

/** 对应 `commands/system.rs::Pong` */
export interface Pong {
  ok: boolean;
  product: string;
  version: string;
  schemaVersion: number;
  latestSchemaVersion: number;
  keySource: string;
  locked: boolean;
  dataDir: string;
  logDir: string;
  serverTime: number;
}

/** 对应 `commands/system.rs::Heartbeat` */
export interface Heartbeat {
  tick: number;
  ts: number;
}

export type AuthType = "password" | "key";

/** 对应 `commands/connections.rs::ConnectionDto`。注意：永远不含密钥材料。 */
export interface ConnectionDto {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authType: AuthType;
  hasPassword: boolean;
  keyPath: string | null;
  hasKeyPassphrase: boolean;
  legacyAlgorithms: boolean;
  lastUsedAt: number | null;
  createdAt: number;
  updatedAt: number;
}

/**
 * 敏感字段的更新语义：
 * - 传 `undefined` = 保持原值；
 * - 传 `""` = 清空；
 * - 传非空字符串 = 覆盖。
 */
export interface ConnectionInput {
  name: string;
  host: string;
  port: number;
  username: string;
  authType: AuthType;
  password?: string;
  keyPath?: string;
  keyPassphrase?: string;
  legacyAlgorithms: boolean;
}

/** 对应 `commands/connections.rs::TestOutcome`（serde 内部标签枚举） */
export type TestOutcome =
  | {
      status: "connected";
      elapsedMs: number;
      hostKeyAlgo: string;
      fingerprint: string;
      newlyTrusted: boolean;
    }
  | { status: "confirmation_required"; hostKeyAlgo: string; fingerprint: string }
  | {
      status: "host_key_changed";
      hostKeyAlgo: string;
      fingerprint: string;
      storedFingerprint: string;
    }
  | { status: "certificate_unsupported" }
  | { status: "failed"; code: string; message: string; detail: string | null };

/** 对应 `events::OutputBatch` */
export interface OutputBatch {
  seq: number;
  data: string;
  /** `data` 的 UTF-8 字节数，ack 必须按这个回传 */
  bytes: number;
}

/** 对应 `ssh::session::SessionInfo` */
export interface SessionInfo {
  sessionId: string;
  connectionId: string;
  title: string;
  hostLabel: string;
  cols: number;
  rows: number;
  createdAt: number;
}

/** `ssh:state` 事件载荷 */
export interface SshStateEvent {
  sessionId: string;
  state: "disconnected";
  reason: string;
}

/** 对应 `sftp::list::RemoteEntry` */
export interface RemoteEntry {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number;
  modifiedMs: number | null;
  /** 原始 Unix mode，低 12 位是权限 */
  mode: number | null;
  uid: number | null;
  gid: number | null;
}

/** 对应 `commands::sftp::SftpOpened` */
export interface SftpOpened {
  sessionId: string;
  home: string;
  elapsedMs: number;
}

/** 对应 `commands::sftp::RemoteStat` */
export interface RemoteStat {
  isDir: boolean;
  size: number;
}

/** `sftp:progress` 事件载荷，对应 `sftp::transfer::TransferProgress` */
export interface TransferProgress {
  transferId: string;
  direction: "upload" | "download";
  name: string;
  transferred: number;
  total: number;
  bps: number;
  state: "running" | "done" | "failed" | "canceled";
  error: string | null;
}

/** 命令片段的风险级别，由 Rust 判定，前端不得降级（SEC-SN-10） */
export type SnippetRisk = "readonly" | "dangerous" | "critical";

/** 对应 `snippets::SnippetDto` */
export interface SnippetDto {
  id: string;
  name: string;
  command: string;
  description: string;
  group: string;
  risk: SnippetRisk;
  /** 非 null 时可以在诊断面板里以表格查看 */
  diag: DiagKind | null;
}

/** 诊断类型，对应 `monitor::DiagKind` */
export type DiagKind = "disk" | "inode" | "memory" | "processes" | "ports" | "process_tree";

/** 对应 `monitor::DiagResult` */
export interface DiagResult {
  kind: DiagKind;
  title: string;
  command: string;
  columns: string[];
  /** 与 `columns` 一一对应 */
  rows: string[][];
  /** 一句人话结论 */
  summary: string;
  /** 需要留意的告警，没有则为 null */
  warning: string | null;
  raw: string;
  fetchedAt: number;
}

/** 终端配色主题，对应 `commands::settings::TerminalTheme` */
export type TerminalTheme = "dark" | "light";

/** 对应 `commands::settings::Settings` */
export interface Settings {
  fontSize: number;
  theme: TerminalTheme;
  /** 终端回滚缓冲行数 */
  scrollback: number;
}
