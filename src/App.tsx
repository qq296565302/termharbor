import { lazy, Suspense, useEffect, useSyncExternalStore } from "react";

import { usePing } from "./hooks/usePing";
import { clearIpcFailure, getIpcFailure, subscribeIpcFailure } from "./lib/ipc";
import { ipcLabel, keySourceLabel } from "./lib/labels";
import ConnectionsPage from "./pages/ConnectionsPage";
import AboutPage from "./pages/AboutPage";
import SettingsPage from "./pages/SettingsPage";
import TerminalPage from "./pages/TerminalPage";
import { useSessionStore } from "./stores/sessionStore";
import { useSettingsStore } from "./stores/settingsStore";
import { useUiStore, type PageId } from "./stores/uiStore";

interface NavItem {
  id: PageId;
  label: string;
}

const NAV: NavItem[] = [
  { id: "connections", label: "连接" },
  { id: "terminal", label: "终端" },
  { id: "settings", label: "设置" },
  { id: "about", label: "关于" },
];

// S3 的输入法取证页只在开发构建里出现。用 import.meta.env.DEV 做守卫，
// 生产构建时 Rollup 会把整个动态 import 分支判死，xterm 不会被包进正式产物。
const ImeTestPage = import.meta.env.DEV ? lazy(() => import("./pages/ImeTestPage")) : null;

/**
 * 自检页只在开发构建里保留。
 *
 * 它原本是 M0 的验收界面（版本/schema/密钥来源/目录 + Channel 心跳 + 已完成清单）。
 * 现在：运行信息在「关于」页已经有了，Channel 的心跳测试也被真正的终端输出取代，
 * M0 清单对使用者没有意义。留着当排障工具，但不占用户的导航位置。
 */
const SelfCheckPage = import.meta.env.DEV
  ? lazy(() => import("./pages/SelfCheckPage"))
  : null;

if (import.meta.env.DEV) {
  NAV.push({ id: "selfcheck", label: "自检" });
  NAV.push({ id: "ime", label: "输入法测试" });
}

const TITLES: Record<PageId, string> = {
  selfcheck: "自检",
  connections: "连接管理",
  terminal: "终端会话",
  settings: "设置",
  about: "关于",
  ime: "输入法测试",
};

function BrandMark() {
  return (
    <svg className="brand__mark" viewBox="0 0 32 32" aria-hidden="true">
      <rect width="32" height="32" rx="8" fill="#0F2A47" />
      <path
        d="M11 9.5 L19.5 16 L11 22.5"
        fill="none"
        stroke="#5DCAA5"
        strokeWidth="2.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <rect x="21" y="19.4" width="5" height="2.6" rx="1.3" fill="#B5D4F4" />
    </svg>
  );
}

export default function App() {
  const { activePage, setActivePage } = useUiStore();
  const sessionCount = useSessionStore((state) => state.tabs.length);
  const { pong, error, loading, refresh } = usePing();
  /** 最近一次「根本没调到后端」的调用（`invoke` 自己抛的错，不是业务错误） */
  const ipcFailure = useSyncExternalStore(subscribeIpcFailure, getIpcFailure);

  const loadSettings = useSettingsStore((state) => state.load);

  // 设置要在终端建立会话之前就位，否则第一个标签会用默认字号
  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  const statusText = error ? "后端异常" : pong ? (pong.locked ? "未解锁" : "就绪") : "连接中";
  const statusTone = error || pong?.locked ? "warn" : pong ? "ok" : "idle";

  // 状态栏里的文案（枚举名 → 人话）。悬停能看到一句解释，不用猜。
  const keySource = keySourceLabel(pong?.keySource);
  const ipc = ipcLabel(error?.code ?? null, ipcFailure);
  // 平时不显示 schema 版本 —— 它只在"库和程序对不上"时有意义
  const schemaDrift = !!pong && pong.schemaVersion !== pong.latestSchemaVersion;

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <BrandMark />
          <div className="brand__text">
            <strong>TermHarbor</strong>
            <span>终端港</span>
          </div>
        </div>

        <nav className="nav">
          {NAV.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`nav__item ${activePage === item.id ? "is-active" : ""}`}
              onClick={() => setActivePage(item.id)}
            >
              {item.label}
            </button>
          ))}
        </nav>

        <div className="sidebar__foot">
          <span className={`dot dot--${statusTone}`} />
          <span>{statusText}</span>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <h1>{TITLES[activePage]}</h1>
          <span className="topbar__meta" title="版本号来自安装包（tauri.conf.json），不是编译期猜的">
            {pong ? `v${pong.version}` : loading ? "…" : "—"}
          </span>
          {schemaDrift && pong ? (
            <span
              className="topbar__warn"
              title={`本地配置库结构是 v${pong.schemaVersion}，当前程序需要 v${pong.latestSchemaVersion}；落后的话重启一次就会自动迁移`}
            >
              配置库 v{pong.schemaVersion} ≠ 程序 v{pong.latestSchemaVersion}
            </span>
          ) : null}
        </header>

        <section className="content">
          {activePage === "selfcheck" && SelfCheckPage ? (
            <Suspense fallback={<div className="skeleton">正在加载自检页…</div>}>
              <SelfCheckPage pong={pong} error={error} loading={loading} refresh={refresh} />
            </Suspense>
          ) : null}

          {activePage === "connections" && <ConnectionsPage />}

          {/* 终端页常驻挂载：切到别的页面时只是隐藏，否则所有会话都会被销毁 */}
          <div className="content__slot" hidden={activePage !== "terminal"}>
            <TerminalPage />
          </div>

          {activePage === "ime" && ImeTestPage ? (
            <Suspense fallback={<div className="skeleton">正在加载终端组件…</div>}>
              <ImeTestPage />
            </Suspense>
          ) : null}

          {activePage === "settings" && <SettingsPage />}

          {activePage === "about" && <AboutPage pong={pong} />}
        </section>

        <footer className="statusbar">
          <span title="已打开的终端标签数，每个标签一条 SSH 连接">会话 {sessionCount}</span>
          <span className="statusbar__sep" />
          <span title={keySource.hint}>密钥 {keySource.text}</span>
          <span className="statusbar__spacer" />
          <span
            className={ipcFailure ? "statusbar__alert" : undefined}
            title={ipc.hint}
            onClick={ipcFailure ? clearIpcFailure : undefined}
          >
            {ipc.text}
          </span>
        </footer>
      </main>
    </div>
  );
}
