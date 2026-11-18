/**
 * 只给 UI 取证用的预览页：把四个侧栏/浮层组件挂在浏览器里，用假数据渲染。
 *
 * 为什么需要：本机执行环境建不起 WebView2，`vite dev` 打开普通浏览器时
 * `isDesktopRuntime()` 为 false，所有 IPC 都会直接抛错 —— 面板只会显示空态，
 * 改完样式根本看不到效果。
 *
 * 做法：塞一个最小的 `__TAURI_INTERNALS__.invoke` 进去，按命令名返回假数据。
 * 不进产品代码，`ui-preview.html` 也不在打包入口里。
 */
import { createRoot } from "react-dom/client";

import ConnectionsPage from "../pages/ConnectionsPage";
import DiagPanel from "../pages/DiagPanel";
import FilePanel from "../pages/FilePanel";
import DbPanel from "../pages/DbPanel";
import LoadPanel from "../pages/LoadPanel";
import McpSettings from "../pages/McpSettings";
import SnippetPanel from "../pages/SnippetPanel";
import "../styles.css";
import type {
  ConnectionDto,
  DiagResult,
  EnvStatus,
  LoadSnapshot,
  McpAuditEntry,
  McpAuditView,
  McpConfigView,
  RemoteEntry,
  SnippetDto,
  TunnelDto,
} from "../lib/types";

const now = Date.now();
const MIN = 60_000;

const entries: RemoteEntry[] = [
  { name: "etc", path: "/etc", isDir: true, isSymlink: false, size: 0, modifiedMs: now - 3 * 60 * MIN, mode: 0o40755, uid: 0, gid: 0 },
  { name: "opt", path: "/opt", isDir: true, isSymlink: false, size: 0, modifiedMs: now - 40 * MIN, mode: 0o40755, uid: 0, gid: 0 },
  { name: "var", path: "/var", isDir: true, isSymlink: false, size: 0, modifiedMs: now - 8 * MIN, mode: 0o40755, uid: 0, gid: 0 },
  { name: "root", path: "/root", isDir: true, isSymlink: true, size: 0, modifiedMs: now - 12 * MIN, mode: 0o40700, uid: 0, gid: 0 },
  { name: "docker-compose.yml", path: "/docker-compose.yml", isDir: false, isSymlink: false, size: 1240, modifiedMs: now - 90 * MIN, mode: 0o100644, uid: 0, gid: 0 },
  { name: "nohup.out", path: "/nohup.out", isDir: false, isSymlink: false, size: 3_481_002, modifiedMs: now - 2 * MIN, mode: 0o100644, uid: 0, gid: 0 },
  { name: "deploy.sh", path: "/deploy.sh", isDir: false, isSymlink: false, size: 862, modifiedMs: now - 26 * 60 * MIN, mode: 0o100755, uid: 0, gid: 0 },
  { name: "server.log", path: "/server.log", isDir: false, isSymlink: false, size: 18_204_991, modifiedMs: now - 40_000, mode: 0o100640, uid: 0, gid: 0 },
];

const snippets: SnippetDto[] = [
  { id: "s1", name: "查看端口占用", command: "ss -tulnp", description: "列出监听端口", group: "排查", risk: "readonly", diag: "ports", builtin: true },
  { id: "s2", name: "磁盘占用", command: "df -h", description: "各挂载点用量", group: "排查", risk: "readonly", diag: "disk", builtin: true },
  { id: "s3", name: "进程树", command: "ps auxf", description: "按树形看进程", group: "排查", risk: "readonly", diag: "processes", builtin: true },
  { id: "s4", name: "重启 nginx", command: "systemctl restart nginx", description: "", group: "运维", risk: "dangerous", diag: null, builtin: true },
  { id: "s5", name: "部署 web", command: "cd /opt/web && git pull && npm ci && npm run build", description: "", group: "我的项目", risk: "dangerous", diag: null, builtin: false },
  { id: "s6", name: "清空日志", command: "truncate -s 0 /var/log/nginx/access.log", description: "", group: "我的项目", risk: "critical", diag: null, builtin: false },
];

const env: EnvStatus[] = [
  { name: "git", installed: true, version: "2.43.0" },
  { name: "docker", installed: true, version: "24.0.7" },
  { name: "node", installed: true, version: "20.11.1" },
  { name: "python3", installed: true, version: "3.11.6" },
  { name: "nginx", installed: false, version: null },
  { name: "redis", installed: false, version: null },
];

const load: LoadSnapshot = {
  os: "Debian GNU/Linux 12",
  kernel: "Linux 6.1.0-18-amd64",
  uptimeSecs: 1_264_320,
  load1: 0.42,
  load5: 0.38,
  load15: 0.31,
  running: 3,
  totalProcs: 142,
  cpu: {
    percent: 18.4,
    coresCount: 4,
    cores: [22.1, 14.8, 19.2, 17.5],
  },
  memory: {
    totalKb: 3_940_000,
    usedKb: 1_820_000,
    availableKb: 2_120_000,
    cachedKb: 920_000,
    buffersKb: 130_000,
    percent: 46.2,
    swapTotalKb: 2_000_000,
    swapUsedKb: 96_000,
    swapPercent: 4.8,
  },
  disks: [
    { filesystem: "/dev/vda1", mount: "/", size: "40G", used: "24G", available: "14G", percent: 63 },
    { filesystem: "/dev/vdb1", mount: "/data", size: "200G", used: "188G", available: "12G", percent: 94 },
  ],
  nets: [
    { name: "eth0", rxBps: 1_240_000, txBps: 320_000, rxTotal: 8_200_000_000, txTotal: 2_100_000_000 },
    { name: "docker0", rxBps: 3_200, txBps: 1_100, rxTotal: 120_000, txTotal: 90_000 },
  ],
  processes: [
    { pid: 1421, name: "node", cpu: 12.4, mem: 8.2 },
    { pid: 884, name: "mysqld", cpu: 6.8, mem: 21.4 },
    { pid: 2510, name: "nginx", cpu: 3.1, mem: 1.2 },
    { pid: 976, name: "redis-server", cpu: 1.4, mem: 2.6 },
    { pid: 1, name: "systemd", cpu: 0.2, mem: 0.4 },
  ],
  sessions: 2,
  tcpEstablished: 38,
  windowMs: 3000,
  warning: null,
};

const diag: DiagResult = {
  kind: "ports",
  title: "监听端口",
  command: "ss -tulnp",
  columns: ["协议", "本地地址", "进程"],
  rows: [
    ["tcp", "0.0.0.0:22", "sshd (884)"],
    ["tcp", "0.0.0.0:80", "nginx (2510)"],
    ["tcp", "0.0.0.0:443", "nginx (2510)"],
    ["tcp", "127.0.0.1:3306", "mysqld (884)"],
    ["udp", "0.0.0.0:53", "systemd-resolve (620)"],
    ["udp", "127.0.0.1:323", "chronyd (701)"],
  ],
  summary: "共 6 个监听端口，其中 3 个对外（22 / 80 / 443），其余只在本机。",
  warning: "注意：3306 监听在 127.0.0.1，外部访问会被拒；要远程连库需要改 bind-address。",
  raw: "Netid State  Local Address:Port  Process\ntcp   LISTEN 0.0.0.0:22         sshd\ntcp   LISTEN 0.0.0.0:80         nginx\n...",
  fetchedAt: now,
};

const responses: Record<string, unknown> = {
  sftp_open: { sessionId: "s1", home: "/root", elapsedMs: 42 },
  sftp_list: entries,
  sftp_parent_of: "/",
  snippet_list: snippets,
  snippet_classify: "readonly",
  env_probe: env,
  load_snapshot: () => nextLoad(),
};

/* ---------------------------------------------------------------- 连接页（跳板取证） */

const connections: ConnectionDto[] = [
  {
    id: "c1",
    name: "堡垒机",
    host: "10.0.0.9",
    port: 2222,
    username: "jump",
    authType: "password",
    hasPassword: true,
    keyPath: null,
    hasKeyPassphrase: false,
    legacyAlgorithms: false,
    jumpConnectionId: null,
    lastUsedAt: now - 40 * MIN,
    createdAt: now - 90 * 24 * 60 * MIN,
    updatedAt: now - 90 * 24 * 60 * MIN,
  },
  {
    id: "c2",
    name: "内网 MySQL",
    host: "10.0.3.21",
    port: 22,
    username: "ops",
    authType: "key",
    hasPassword: false,
    keyPath: "C:/Users/me/.ssh/id_ed25519",
    hasKeyPassphrase: true,
    legacyAlgorithms: false,
    jumpConnectionId: "c1",
    lastUsedAt: now - 2 * MIN,
    createdAt: now - 30 * 24 * 60 * MIN,
    updatedAt: now - 6 * 24 * 60 * MIN,
  },
  {
    id: "c3",
    name: "老 CentOS 6",
    host: "10.0.0.31",
    port: 22,
    username: "root",
    authType: "password",
    hasPassword: true,
    keyPath: null,
    hasKeyPassphrase: false,
    legacyAlgorithms: true,
    jumpConnectionId: null,
    lastUsedAt: null,
    createdAt: now - 3 * 24 * 60 * MIN,
    updatedAt: now - 3 * 24 * 60 * MIN,
  },
];

/** 导入预览夹具：两条带跳板名字的，一条能按名字唯一匹配、一条匹配不上 */
const importPreview = {
  entries: [
    {
      key: "10.0.3.21|22|ops",
      name: "内网 MySQL",
      host: "10.0.3.21",
      port: 22,
      username: "ops",
      authType: "key",
      jumpName: "堡垒机",
      duplicate: false,
      suggested: "c1",
    },
    {
      key: "10.0.3.30|22|ops",
      name: "内网 API",
      host: "10.0.3.30",
      port: 22,
      username: "ops",
      authType: "password",
      jumpName: "公司堡垒机",
      duplicate: false,
      suggested: null,
    },
  ],
  connections: connections.map((rec) => ({
    id: rec.id,
    name: rec.name,
    host: rec.host,
    port: rec.port,
    username: rec.username,
  })),
};

/** 转发规则夹具：运行中 / 未启动 / 重连中 / 失败，自动启动开关也要有开有关 */
const tunnelFixtures: TunnelDto[] = [
  {
    id: "t1",
    connectionId: "c2",
    localPort: 13306,
    targetHost: "10.0.3.21",
    targetPort: 3306,
    autoStart: true,
    state: "listening",
    message: null,
    createdAt: now - 3 * 60 * MIN,
  },
  {
    id: "t2",
    connectionId: "c2",
    localPort: 16379,
    targetHost: "10.0.3.21",
    targetPort: 6379,
    autoStart: false,
    state: "stopped",
    message: null,
    createdAt: now - 2 * 60 * MIN,
  },
  {
    id: "t3",
    connectionId: "c2",
    localPort: 15432,
    targetHost: "10.0.3.21",
    targetPort: 5432,
    autoStart: true,
    state: "reconnecting",
    message: "连接已断开，正在重连",
    createdAt: now - 5 * 60 * MIN,
  },
  {
    id: "t4",
    connectionId: "c2",
    localPort: 13307,
    targetHost: "47.94.196.217",
    targetPort: 13306,
    autoStart: false,
    state: "failed",
    message:
      "远端连不上 47.94.196.217:13306（Failed to open channel (ConnectFailed)）。目标填的是这台服务器自己的公网 IP —— 云主机通常连不回自己的公网地址，换成 127.0.0.1 或内网 IP",
    createdAt: now - 60 * MIN,
  },
];

Object.assign(responses, {
  tunnel_list: tunnelFixtures,
  tunnel_create: tunnelFixtures[1],
  tunnel_start: tunnelFixtures[0],
  tunnel_stop: tunnelFixtures[1],
  db_account_get: {
    configured: true,
    username: "root",
    port: 13036,
    database: "sgcc_ops",
  },
  db_schema: {
    database: "sgcc_ops",
    tables: [
      { name: "devices", columns: ["id", "name", "status", "amount", "created_at"] },
      { name: "device_logs", columns: ["id", "device_id", "level", "message", "at"] },
      { name: "stations", columns: ["id", "code", "region", "capacity"] },
      { name: "alarms", columns: ["id", "device_id", "kind", "raised_at", "cleared_at"] },
      { name: "users", columns: ["id", "email", "role", "created_at"] },
      { name: "audit_trail", columns: ["id", "actor", "action", "target", "at"] },
      { name: "daily_stats", columns: ["day", "device_id", "uptime", "errors"] },
      { name: "maintenance", columns: ["id", "device_id", "plan_at", "done_at", "note"] },
    ],
    truncated: false,
  },
  // 含一个 NULL：验证 is-null 的灰色斜体，别让空值和字符串 "NULL" 看不出来
  db_query: {
    columns: ["id", "name", "status", "amount", "created_at"],
    rows: [
      ["10231", "华东-配电柜A3", "active", "12800.00", "2026-11-17 22:14:08"],
      ["10230", "华北-环网柜B1", "active", "9600.00", "2026-11-17 21:02:41"],
      ["10229", "西南-变压器T7", "NULL", "0.00", "2026-11-17 19:48:12"],
      ["10228", "华南-开关站C2", "active", "23400.50", "2026-11-17 18:31:07"],
      ["10227", "东北-电缆段D9", "archived", "5400.00", "2026-11-17 16:55:20"],
    ],
    truncated: false,
    elapsedMs: 23,
  },
  tunnel_set_auto_start: tunnelFixtures[0],
  tunnel_delete: null,
  "plugin:dialog|open": "C:/Users/me/Desktop/termharbor-connections.json",
  config_import_preview: importPreview,
  config_import: { imported: 2, skipped: 0 },
  connection_list: connections,
  connection_get: connections[1],
  connection_create: connections[1],
  connection_update: connections[1],
  connection_delete: null,
});

/* ---------------------------------------------------------------- MCP 设置页 */

const mcpView: McpConfigView = {
  config: {
    version: 2,
    defaultEnabled: false,
    requireLiveSession: true,
    hostToken: "a1b2c3d4",
    approvalTtlMinutes: 0,
    approvals: [],
    connections: {
      c1: {
        enabled: true,
        tools: [],
        paths: [],
        allowSecretFiles: false,
        fullTrust: false,
        allowExec: true,
        allowWrite: true,
        allowUpload: true,
        exec: { allow: [], timeoutSecs: 60, maxOutputKb: 64 },
      },
      c2: {
        enabled: true,
        tools: ["sftp_list", "exec"],
        paths: ["/var/log"],
        allowSecretFiles: false,
        fullTrust: true,
        allowExec: false,
        allowWrite: true,
        allowUpload: true,
        exec: { allow: ["df -h", "journalctl -n 50"], timeoutSecs: 60, maxOutputKb: 64 },
      },
      c3: {
        enabled: false,
        tools: [],
        paths: [],
        allowSecretFiles: false,
        fullTrust: false,
        allowExec: true,
        allowWrite: true,
        allowUpload: true,
        exec: { allow: [], timeoutSecs: 60, maxOutputKb: 64 },
      },
    },
  },
  connections: [
    { id: "c1", name: "prod-web-01", host: "10.0.0.11", port: 22 },
    { id: "c2", name: "prod-db-01", host: "10.0.0.21", port: 22 },
    { id: "c3", name: "staging", host: "10.0.0.31", port: 2222 },
  ],
  commandTemplates: ["df -h", "free -m", "uptime", "docker ps"],
  configPath: "%APPDATA%\\TermHarbor\\mcp.json",
  auditPath: "%APPDATA%\\TermHarbor\\audit.jsonl",
};

/** 当前「客户端进程」的 pid：夹具里用它区分「本进程写的租约」和别人的 */
const SELF_PID = 8842;

const mcpDenied: McpAuditEntry[] = [
  {
    tsMs: now - 320_000,
    client: "claude-ai",
    tool: "exec",
    connection: "prod-web-01",
    args: "df -h",
    decision: "allowed",
    reason: "-",
    durationMs: 412,
    bytes: 620,
    result: "ok",
    lease: { pid: SELF_PID, beatMs: now - 322_200, ageMs: 2_200 },
  },
  {
    tsMs: now - 200_000,
    client: "claude-ai",
    tool: "exec",
    connection: "prod-db-01",
    args: "mysql -p*** -e 'show databases'",
    decision: "denied",
    reason: "command_not_allowed",
    durationMs: 0,
    bytes: 0,
    result: "denied",
    lease: { pid: SELF_PID, beatMs: now - 201_100, ageMs: 1_100 },
  },
  {
    tsMs: now - 140_000,
    client: "claude-ai",
    tool: "sftp_list",
    connection: "prod-web-01",
    args: '{"connection":"prod-web-01","path":"/var/log"}',
    decision: "allowed",
    reason: "-",
    durationMs: 168,
    bytes: 1_840,
    result: "ok",
    lease: { pid: SELF_PID, beatMs: now - 141_800, ageMs: 1_800 },
  },
  {
    // 非本进程写的租约：界面上要标黄（夹具故意用另一个 pid）
    tsMs: now - 60_000,
    client: "claude-ai",
    tool: "exec",
    connection: "prod-web-01",
    args: "uptime",
    decision: "allowed",
    reason: "-",
    durationMs: 388,
    bytes: 92,
    result: "ok",
    lease: { pid: 1234, beatMs: now - 60_300, ageMs: 300 },
  },
  {
    // 读的时候压根没有租约文件：指纹为空，原因写 no_live_session
    tsMs: now - 20_000,
    client: "claude-ai",
    tool: "exec",
    connection: "prod-db-01",
    args: "uptime",
    decision: "denied",
    reason: "no_live_session",
    durationMs: 0,
    bytes: 0,
    result: "denied",
    lease: null,
  },
];

const mcpAudit: McpAuditView = {
  entries: mcpDenied,
  selfPid: SELF_PID,
  startedAtMs: now - 3_600_000,
};

Object.assign(responses, {
  mcp_config_get: mcpView,
  mcp_audit_recent: mcpAudit,
  // 只有 c1 在线：另一台显示「未连接」，一眼看出这道闸门在起作用
  mcp_live_connections: ["c1"],
  mcp_config_set: null,
});

interface Internals {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  transformCallback: (cb: unknown) => number;
  metadata: Record<string, unknown>;
}

let seq = 0;
(window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__ = {
  invoke: async (cmd) => {
    // 事件通道只要求返回一个 id；其余按命令名查表
    if (cmd.startsWith("plugin:event|")) return 1;
    if (cmd in responses) {
      const value = responses[cmd];
      // 函数就每次重算：看板要靠多次采样才有曲线，固定值只能得到一条直线
      return typeof value === "function" ? value() : value;
    }
    return null;
  },
  transformCallback: () => {
    seq += 1;
    return seq;
  },
  metadata: {
    currentWindow: { label: "main" },
    currentWebview: { label: "main", windowLabel: "main" },
  },
};

const panel = new URLSearchParams(window.location.search).get("panel");

function Preview() {
  if (panel === "diag") {
    return (
      <DiagPanel
        title={diag.title}
        loading={false}
        error={null}
        result={diag}
        onRefresh={() => undefined}
        onClose={() => undefined}
      />
    );
  }

  if (panel === "load") {
    return (
      <div style={{ width: "100%", height: "100vh", display: "flex", justifyContent: "flex-end" }}>
        <LoadPanel sessionId="s1" active onClose={() => undefined} />
      </div>
    );
  }

  if (panel === "db") {
    return (
      <div style={{ width: "100%", height: "100vh", display: "flex", justifyContent: "flex-end" }}>
        <DbPanel connectionId="c1" onClose={() => undefined} />
      </div>
    );
  }

  if (panel === "connections") {
    return (
      <div style={{ height: "100vh", display: "flex", flexDirection: "column" }}>
        <ConnectionsPage />
      </div>
    );
  }

  if (panel === "mcp") {
    return (
      <div className="settings-page" style={{ height: "100vh", overflowY: "auto", padding: 24 }}>
        <McpSettings />
      </div>
    );
  }

  return (
    <div style={{ display: "flex", height: "100vh" }}>
      <div style={{ display: "flex", flex: "0 0 860px", minWidth: 0 }}>
        <div style={{ display: "flex", flex: 1, minWidth: 0, flexDirection: "column" }}>
          <FilePanel connectionId="c1" />
        </div>
        <SnippetPanel
          onInsert={() => undefined}
          onView={() => undefined}
          onClose={() => undefined}
          sessionId="s1"
        />
      </div>
      <LoadPanel sessionId="s1" active onClose={() => undefined} />
    </div>
  );
}

/**
 * 采样夹具：每次调用往前走一格，按正弦 + 抖动造出"像真的"负载曲线。
 *
 * 固定值在预览里只能画出一条直线 —— 而这块面板这次改的就是"看不出波动"，
 * 用直线截图等于什么都没验证。
 */
let loadTick = 0;
function nextLoad(): LoadSnapshot {
  loadTick += 1;
  const wave = Math.sin(loadTick / 2.2);
  const jitter = () => (Math.random() - 0.5) * 6;

  const cpu = Math.max(3, Math.min(96, 42 + wave * 26 + jitter()));
  const coreCount = load.cpu.cores.length;
  const cores = load.cpu.cores.map((_, index) =>
    Math.max(1, Math.min(100, cpu + Math.sin(loadTick / 1.6 + index) * 14 + (Math.random() - 0.5) * 8)),
  );
  const memPercent = Math.min(94, 46 + loadTick * 0.6);
  const rx = 900_000 + wave * 520_000 + Math.random() * 160_000;
  const tx = 240_000 + Math.cos(loadTick / 3) * 120_000 + Math.random() * 50_000;

  return {
    ...load,
    load1: Math.max(0.05, 0.6 + wave * 0.5),
    load5: 0.5 + wave * 0.3,
    load15: 0.42 + wave * 0.15,
    cpu: { percent: cpu, coresCount: coreCount, cores },
    memory: {
      ...load.memory,
      percent: memPercent,
      usedKb: Math.round(load.memory.totalKb * (memPercent / 100)),
      availableKb: Math.round(load.memory.totalKb * (1 - memPercent / 100)),
    },
    nets: [
      { ...load.nets[0], rxBps: rx, txBps: tx },
      load.nets[1],
    ],
    tcpEstablished: 18 + Math.round(Math.abs(wave) * 12),
  };
}

createRoot(document.getElementById("root")!).render(<Preview />);
