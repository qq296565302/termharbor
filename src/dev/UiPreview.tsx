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

import DiagPanel from "../pages/DiagPanel";
import FilePanel from "../pages/FilePanel";
import LoadPanel from "../pages/LoadPanel";
import SnippetPanel from "../pages/SnippetPanel";
import "../styles.css";
import type {
  DiagResult,
  EnvStatus,
  LoadSnapshot,
  RemoteEntry,
  SnippetDto,
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
  load_snapshot: load,
};

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
    if (cmd in responses) return responses[cmd];
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

createRoot(document.getElementById("root")!).render(<Preview />);
